# tern-pdf

A native Tern PDF pane for side-by-side LaTeX and Typst editing. Luau owns the pane; a Rust helper uses Poppler to render PDF pages. The preview watches the PDF output, independently of your editor or build command.

## Install

Requirements on the machine holding the PDF:

- Tern with the [plugin SDK](https://docs.stencil.so/tern/).
- Rust 1.89+ and Cargo to build the helper.
- Poppler's `pdfinfo` and `pdftoppm` on the Tern daemon's `PATH` (`poppler-utils` on Debian/Ubuntu, `poppler` on Arch or Homebrew).

From this checkout:

```sh
cargo build --release
tern plugin link .
```

Keep the checkout in place: the plugin invokes `target/release/tern-pdf` inside it. Rebuild that binary after changing Rust code. Tern reloads plugin source changes; `tern plugin reload pdf` reloads explicitly. If Poppler was installed after starting the daemon, ensure the daemon can find it on its existing `PATH`.

## Editing

Run your editor and build watcher in terminal panes, then open the PDF:

```sh
# Typst
typst watch paper.typ paper.pdf

# Or LaTeX
latexmk -pdf -pvc -view=none paper.tex

# From another Tern terminal pane
tern open paper.pdf
```

PDF opens are routed to the PDF Viewer block. The Files pane also offers **Open with PDF Viewer**. For remote PDFs, install the plugin and Poppler on that host; rendering happens beside the file, not on the viewing client.

## Controls

| Key / gesture | Action |
| --- | --- |
| `←` / `p` / Page Up | Previous page |
| `→` / `n` / Page Down | Next page |
| Home / End | First / last page |
| `g`, then a number, Enter | Go to page (typing a digit also starts entry) |
| Escape | Cancel page entry |
| `f` | Toggle fit-width / fit-page and reset zoom |
| `+` / `-` | Zoom the page in / out inside the pane (50–400% of pane width) |
| `r` | Check the output file immediately |
| Mouse wheel / trackpad | Scroll vertically; horizontal trackpad/wheel input pans across a zoomed page |
| Click page | Open Tern's image zoom viewer |

Zoom changes the displayed page immediately. Render resolution follows automatically, up to 4096 pixels per side; the previous image stays visible while sharper pixels load. Manual zoom switches to width mode, where 100% means fit-width, not physical PDF size. Zoom and fit mode are saved with the pane. The dock reports loading and rendering errors. Tern's window-level shortcuts take precedence over pane keys.


## Reload behavior

The pane polls through short-lived helper commands. An unchanged PDF reuses its cached page without invoking Poppler. A changed file is allowed to settle, copied to a private snapshot, and rendered before replacing the preview. In-place writes and atomic file replacements are supported. Failed or incomplete builds leave the previous image visible and are retried on subsequent polls.

The current page survives rebuilds and is clamped when the document gets shorter. Explicit image dimensions reserve the page's geometry while a replacement blob decodes, preserving horizontal and vertical scroll offsets for same-size rebuilds; a shorter page can naturally clamp the scroll. Document path, page, zoom and viewing mode are saved with the pane. The helper retains at most six page images and one committed PDF snapshot per pane. Cache data lives under Tern's state directory at `plugin-data/pdf/pane-<id>` and can be removed when the pane is closed.

## Limits

- Pages are raster images: no on-page text selection, annotations, PDF links, or SyncTeX/source navigation.
- Rendered images are limited to 4096 pixels per side by Tern. Zoom still enlarges the page beyond that resolution, but it cannot add detail.
- The plugin does not compile documents or report compiler diagnostics; keep your build watcher's output visible.
- Local iOS rendering is unsupported because the helper needs subprocesses; a desktop/remote daemon can render for its clients.
