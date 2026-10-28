use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEBOUNCE_MS: u64 = 150;
const MAX_IMAGES: usize = 6;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Signature {
    len: u64,
    modified_ns: u128,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    changed_ns: i128,
}

fn signature(path: &Path) -> Result<Signature, String> {
    let m = fs::metadata(path).map_err(|e| format!("cannot stat source: {e}"))?;
    if !m.is_file() {
        return Err("source is not a regular file".into());
    }
    let modified_ns = m
        .modified()
        .map_err(|e| format!("cannot read source modification time: {e}"))?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "source modification time is before the epoch")?
        .as_nanos();
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Ok(Signature {
        len: m.len(),
        modified_ns,
        #[cfg(unix)]
        dev: m.dev(),
        #[cfg(unix)]
        ino: m.ino(),
        #[cfg(unix)]
        changed_ns: i128::from(m.ctime()) * 1_000_000_000 + i128::from(m.ctime_nsec()),
    })
}

#[derive(Clone, Serialize, Deserialize)]
struct Image {
    page: u32,
    scale: u32,
    file: String,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    source: String,
    observed: Option<Signature>,
    observed_at_ms: u64,
    committed: Option<Signature>,
    generation: String,
    pages: u32,
    images: Vec<Image>,
}

#[derive(Serialize)]
struct Response {
    generation: String,
    page: u32,
    pages: u32,
    image: String,
    changed: bool,
}

struct Temps(Vec<PathBuf>);
impl Drop for Temps {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn temp_path(cache: &Path, suffix: &str) -> PathBuf {
    cache.join(format!(".tmp-{}-{}-{suffix}", process::id(), now_ms()))
}

fn save_state(cache: &Path, state: &State) -> Result<(), String> {
    let tmp = temp_path(cache, "state");
    let _cleanup = Temps(vec![tmp.clone()]);
    let mut file = File::create(&tmp).map_err(|e| format!("cannot create cache metadata: {e}"))?;
    serde_json::to_writer(&mut file, state)
        .map_err(|e| format!("cannot serialize cache metadata: {e}"))?;
    file.flush()
        .map_err(|e| format!("cannot write cache metadata: {e}"))?;
    fs::rename(&tmp, cache.join("state.json"))
        .map_err(|e| format!("cannot commit cache metadata: {e}"))
}

fn run(program: &str, args: &[OsString]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{program} failed: {}", detail.trim()));
    }
    String::from_utf8(output.stdout).map_err(|_| format!("{program} returned non-UTF-8 output"))
}

fn page_count(snapshot: &Path) -> Result<u32, String> {
    let text = run("pdfinfo", &[snapshot.as_os_str().to_owned()])?;
    let pages = text
        .lines()
        .find_map(|line| line.strip_prefix("Pages:").map(str::trim))
        .ok_or("pdfinfo did not report a page count")?
        .parse::<u32>()
        .map_err(|_| "pdfinfo reported an invalid page count")?;
    if pages == 0 {
        return Err("PDF has no pages".into());
    }
    Ok(pages)
}

fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("cannot open snapshot: {e}"))?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("cannot read snapshot: {e}"))?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn cleanup(cache: &Path, state: &State) {
    if let Ok(entries) = fs::read_dir(cache) {
        let snapshot = format!("snapshot-{}.pdf", state.generation);
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let obsolete = name.starts_with(".tmp-")
                || (name.starts_with("snapshot-") && name.ends_with(".pdf") && name != snapshot)
                || (name.starts_with("image-")
                    && name.ends_with(".png")
                    && !state.images.iter().any(|i| i.file == name));
            if obsolete {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

fn poll(
    source: PathBuf,
    cache: PathBuf,
    requested_page: u32,
    scale: u32,
) -> Result<Response, String> {
    let source = fs::canonicalize(source).map_err(|e| format!("cannot resolve source: {e}"))?;
    fs::create_dir_all(&cache).map_err(|e| format!("cannot create cache directory: {e}"))?;
    let cache =
        fs::canonicalize(cache).map_err(|e| format!("cannot resolve cache directory: {e}"))?;
    // A plugin reload can overlap a helper launched by the old worker.
    // Hold an OS lock so neither process cleans up the other's staged files.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(cache.join(".lock"))
        .map_err(|e| format!("cannot open cache lock: {e}"))?;
    lock.try_lock().map_err(|e| format!("cache is busy: {e}"))?;
    let source_name = source
        .to_str()
        .ok_or("source path is not UTF-8")?
        .to_owned();
    let state_path = cache.join("state.json");
    let mut state: State = match fs::read(&state_path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => State::default(),
        Err(e) => return Err(format!("cannot read cache metadata: {e}")),
    };
    if state.source != source_name {
        state = State {
            source: source_name,
            ..State::default()
        };
    }
    cleanup(&cache, &state);
    let sig = signature(&source)?;
    let snapshot_path = cache.join(format!("snapshot-{}.pdf", state.generation));
    let needs_snapshot =
        state.committed.as_ref() != Some(&sig) || state.pages == 0 || !snapshot_path.is_file();
    if needs_snapshot {
        if state.observed.as_ref() != Some(&sig) {
            state.observed = Some(sig.clone());
            state.observed_at_ms = now_ms();
            save_state(&cache, &state)?;
            return Err("source is changing; waiting for it to settle".into());
        }
        if now_ms().saturating_sub(state.observed_at_ms) < DEBOUNCE_MS {
            return Err("source is changing; waiting for it to settle".into());
        }
    }

    let snapshot_tmp = temp_path(&cache, "snapshot.pdf");
    let render_prefix = temp_path(&cache, "render");
    let render_tmp = render_prefix.with_extension("png");
    let _temps = Temps(vec![snapshot_tmp.clone(), render_tmp.clone()]);
    let (snapshot, generation, pages) = if needs_snapshot {
        fs::copy(&source, &snapshot_tmp).map_err(|e| format!("cannot copy source: {e}"))?;
        if signature(&source)? != sig {
            return Err("source changed while copying".into());
        }
        let generation = hash_file(&snapshot_tmp)?;
        let pages = page_count(&snapshot_tmp)?;
        (snapshot_tmp.clone(), generation, pages)
    } else {
        (snapshot_path, state.generation.clone(), state.pages)
    };
    let page = requested_page.min(pages);
    if !needs_snapshot {
        if let Some(index) = state
            .images
            .iter()
            .position(|i| i.page == page && i.scale == scale && cache.join(&i.file).is_file())
        {
            let path = cache.join(&state.images[index].file);
            if index + 1 != state.images.len() {
                let image = state.images.remove(index);
                state.images.push(image);
                save_state(&cache, &state)?;
            }
            return Ok(Response {
                generation,
                page,
                pages,
                image: path.to_str().ok_or("cache path is not UTF-8")?.to_owned(),
                changed: false,
            });
        }
    }
    run(
        "pdftoppm",
        &[
            "-f".into(),
            page.to_string().into(),
            "-l".into(),
            page.to_string().into(),
            "-singlefile".into(),
            "-png".into(),
            "-scale-to".into(),
            scale.to_string().into(),
            snapshot.as_os_str().to_owned(),
            render_prefix.as_os_str().to_owned(),
        ],
    )?;
    if !render_tmp.is_file() {
        return Err("pdftoppm did not produce an image".into());
    }
    if signature(&source)? != sig {
        return Err("source changed while rendering".into());
    }
    let image_name = format!("image-{generation}-{page}-{scale}.png");
    let image_path = cache.join(&image_name);
    if needs_snapshot {
        let final_snapshot = cache.join(format!("snapshot-{generation}.pdf"));
        fs::rename(&snapshot_tmp, final_snapshot)
            .map_err(|e| format!("cannot commit snapshot: {e}"))?;
    }
    fs::rename(&render_tmp, &image_path).map_err(|e| format!("cannot commit image: {e}"))?;
    if needs_snapshot && state.generation != generation {
        state.images.clear();
    }
    state
        .images
        .retain(|i| !(i.page == page && i.scale == scale));
    state.images.push(Image {
        page,
        scale,
        file: image_name,
    });
    if state.images.len() > MAX_IMAGES {
        state.images.drain(..state.images.len() - MAX_IMAGES);
    }
    state.generation = generation.clone();
    state.pages = pages;
    state.committed = Some(sig.clone());
    state.observed = Some(sig);
    save_state(&cache, &state)?;
    cleanup(&cache, &state);
    Ok(Response {
        generation,
        page,
        pages,
        image: image_path
            .to_str()
            .ok_or("cache path is not UTF-8")?
            .to_owned(),
        changed: true,
    })
}

fn parse_args() -> Result<(PathBuf, PathBuf, u32, u32), String> {
    let mut args = env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("poll")) {
        return Err("usage: tern-pdf poll --source PATH --cache DIR --page N --scale N".into());
    }
    let (mut source, mut cache, mut page, mut scale) = (None, None, None, None);
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {}", flag.to_string_lossy()))?;
        match flag.to_str() {
            Some("--source") if source.is_none() => source = Some(PathBuf::from(value)),
            Some("--cache") if cache.is_none() => cache = Some(PathBuf::from(value)),
            Some("--page") if page.is_none() => {
                page = Some(
                    value
                        .to_str()
                        .ok_or("invalid page")?
                        .parse::<u32>()
                        .map_err(|_| "invalid page")?,
                )
            }
            Some("--scale") if scale.is_none() => {
                scale = Some(
                    value
                        .to_str()
                        .ok_or("invalid scale")?
                        .parse::<u32>()
                        .map_err(|_| "invalid scale")?,
                )
            }
            _ => {
                return Err(format!(
                    "unknown or duplicate argument: {}",
                    flag.to_string_lossy()
                ))
            }
        }
    }
    let page = page.ok_or("missing --page")?;
    let scale = scale.ok_or("missing --scale")?;
    if page == 0 {
        return Err("page must be at least 1".into());
    }
    if !(1..=4096).contains(&scale) {
        return Err("scale must be between 1 and 4096".into());
    }
    Ok((
        source.ok_or("missing --source")?,
        cache.ok_or("missing --cache")?,
        page,
        scale,
    ))
}

fn main() {
    let result =
        parse_args().and_then(|(source, cache, page, scale)| poll(source, cache, page, scale));
    match result {
        Ok(response) => match serde_json::to_string(&response) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("cannot encode response: {e}");
                process::exit(1);
            }
        },
        Err(e) => {
            eprintln!("{e}");
            process::exit(1);
        }
    }
}
