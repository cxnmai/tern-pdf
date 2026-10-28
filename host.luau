local ui = tern.ui
local helper = tern.plugin.dir .. "/target/release/tern-pdf"
local interval = 350
local maxImageBytes = 32 * 1024 * 1024
local zoomLevels = { 50, 67, 80, 100, 125, 150, 200, 250, 300, 400 }

local function restoredZoom(value)
	for _, zoom in zoomLevels do
		if zoom == value then return zoom end
	end
	return 100
end

local function pngDimension(bytes, offset)
	local a, b, c, d = string.byte(bytes, offset, offset + 3)
	return ((a * 256 + b) * 256 + c) * 256 + d
end

local function running(pane)
	for _, p in tern.pane.list() do
		if p.pane == pane then
			return true
		end
	end
	return false
end

local function clamp(value, low, high)
	return math.max(low, math.min(high, value))
end

tern.block.define("viewer", {
	init = function(cx, args, saved)
		local restored = if type(saved) == "table" then saved else {}
		local path = if type(restored.path) == "string" and restored.path ~= "" then restored.path else args[1]
		if path and string.sub(path, 1, 1) ~= "/" and cx.cwd then
			path = cx.cwd .. "/" .. path
		end
		local page = if type(restored.page) == "number" then math.floor(restored.page) else 1
		local zoom = restoredZoom(restored.zoom)
		local state = {
			path = path,
			page = math.max(1, page),
			pages = nil,
			zoom = zoom,
			scale = clamp(math.floor(2000 * zoom / 100), 1000, 4096),
			mode = if restored.mode == "page" then "page" else "width",
			blob = nil,
			image = nil,
			width = nil,
			height = nil,
			displayedPage = nil,
			error = nil,
			notice = nil,
			entry = nil,
			revision = 0,
			busy = false,
			timer = nil,
		}
		if not path or path == "" then
			state.error = "Open a PDF file to start previewing it."
			return state
		end
		if string.sub(path, 1, 1) ~= "/" then
			state.error = "The PDF path must be absolute (no working directory is available)."
			return state
		end

		local launch
		local function schedule()
			if not running(cx.pane) then return end
			state.timer = tern.timer(interval, function()
				state.timer = nil
				if running(cx.pane) then launch() end
			end)
		end
		launch = function()
			if state.busy or not running(cx.pane) then return end
			if state.timer then
				state.timer:cancel()
				state.timer = nil
			end
			state.busy = true
			local requestedPage, requestedScale, revision = state.page, state.scale, state.revision
			local argv = { helper, "poll", "--source", state.path, "--cache", tern.plugin.data .. "/pane-" .. tostring(cx.pane), "--page", tostring(requestedPage), "--scale", tostring(requestedScale) }
			local function finish(result, launchError)
				state.busy = false
				if not running(cx.pane) then return end
				if revision ~= state.revision or requestedPage ~= state.page or requestedScale ~= state.scale then
					launch()
					return
				end
				local oldError, oldNotice, oldPage, oldPages, oldBlob = state.error, state.notice, state.page, state.pages, state.blob
				state.notice = nil
				if launchError then
					state.error = "Cannot start PDF helper at " .. helper .. ": " .. tostring(launchError) .. ". Build the release helper first."
				elseif result.status ~= 0 then
					local message = string.gsub(result.stderr or "", "%s+$", "")
					if message == "source is changing; waiting for it to settle" then
						state.error = nil
						state.notice = "Updating PDF…"
					else
						state.error = if message ~= "" then message else "PDF helper failed (status " .. tostring(result.status) .. ")."
					end
				else
					local ok, data = pcall(tern.json.decode, result.stdout)
					if not ok or type(data) ~= "table" or type(data.generation) ~= "string" or type(data.page) ~= "number" or type(data.pages) ~= "number" or data.pages < 1 or data.page < 1 or data.page > data.pages or type(data.image) ~= "string" or string.sub(data.image, 1, 1) ~= "/" or type(data.changed) ~= "boolean" then
						state.error = "PDF helper returned an invalid response."
					else
						state.page, state.pages = data.page, data.pages
						if data.changed or state.blob == nil or state.image ~= data.image then
							local readOk, bytes = pcall(tern.fs.read, data.image, maxImageBytes)
							if readOk and #bytes >= 24 and string.sub(bytes, 1, 8) == "\137PNG\r\n\26\n" then
								local blobOk, blob = pcall(function() return cx:blob(bytes, "image/png") end)
								if blobOk then
									state.blob, state.image, state.displayedPage = blob, data.image, data.page
									-- Reserve the page's geometry while the new blob decodes.
									state.width, state.height = pngDimension(bytes, 17), pngDimension(bytes, 21)
									state.error = nil
								else
									state.image = nil
									state.error = "Cannot publish PDF page: " .. tostring(blob)
								end
							else
								state.image = nil
								state.error = "Cannot read rendered PDF page: " .. tostring(bytes)
							end
						else
							state.displayedPage = data.page
							state.error = nil
						end
					end
				end
				if state.error ~= oldError or state.notice ~= oldNotice or state.page ~= oldPage or state.pages ~= oldPages or state.blob ~= oldBlob then
					cx:render()
				end
				schedule()
			end
			local ok, err = pcall(function()
				tern.process.run(argv, { timeout_ms = 15000 }, function(result)
					finish(result, nil)
				end)
			end)
			if not ok then finish(nil, err) end
		end
		state.refresh = launch
		launch()
		return state
	end,
	title = function(state)
		if state.path then return string.match(state.path, "([^/]+)$") or "PDF Preview" end
		return "PDF Preview"
	end,
	view = function(state, cx)
		local image = ui.node("image", {
			key = "page", role = "pdf.page", blob = state.blob,
			w = state.width, h = state.height,
			alt = "PDF page", max = if state.mode == "page" then { h = tostring(math.max(1, cx.rows - 2)) .. "lines" } else nil,
		})
		local message = state.error or state.notice or (if state.blob == nil then "Loading PDF…" elseif state.displayedPage ~= state.page then "Loading page " .. tostring(state.page) .. "…" else "")
		local status = if state.entry ~= nil then "Go to page: " .. state.entry .. " (Enter to confirm, Esc to cancel)" else
			"Page " .. tostring(state.page) .. " / " .. (if state.pages then tostring(state.pages) else "?") .. " · " .. (if state.mode == "page" then "fit page" else tostring(state.zoom) .. "% width") .. " · ←/→ pages · f fit · +/- zoom · r refresh"
		local text = ui.text({ ui.span(message, if state.error then "error" else "muted") })
		text.p = text.p or {}
		text.p.key = "message"
		text.p.hidden = message == ""
		local info = ui.text({ ui.span(status, "muted") })
		info.p = info.p or {}
		info.p.role = "pdf.status"
		local pageBox = ui.el("div", {
			key = "pagebox", role = "pdf.pagebox",
			class = "pdf-zoom-" .. tostring(if state.mode == "page" then 100 else state.zoom),
		}, { image })
		local viewport = ui.el("div", { key = "viewport", role = "pdf.viewport" }, { pageBox })
		return { main = ui.col({ viewport }), dock = ui.col({ info, text }) }
	end,
	key = function(state, key, _cx)
		if key.ctrl or key.alt or key.meta then return false end
		local name = key.name
		if state.entry ~= nil then
			if name == "escape" then state.entry = nil return true end
			if name == "backspace" then state.entry = string.sub(state.entry, 1, -2) return true end
			if name == "enter" then
				local target = tonumber(state.entry)
				state.entry = nil
				if target then
					state.page = clamp(math.floor(target), 1, state.pages or math.max(1, math.floor(target)))
					state.revision += 1
					if state.refresh then state.refresh() end
				end
				return true
			end
			if key.text and string.match(key.text, "^%d+$") and #state.entry + #key.text <= 9 then
				state.entry ..= key.text
				return true
			end
			return false
		end
		if name == "g" then state.entry = "" return true end
		if key.text and string.match(key.text, "^%d$") then state.entry = key.text return true end
		if name == "f" then
			state.mode = if state.mode == "page" then "width" else "page"
			state.zoom = 100
			state.scale = 2000
			state.revision += 1
			if state.refresh then state.refresh() end
			return true
		end
		local direction = if name == "+" or key.text == "+" or name == "=" then 1 elseif name == "-" or key.text == "-" then -1 else 0
		if direction ~= 0 then
			local index = table.find(zoomLevels, state.zoom) or 4
			local zoom = zoomLevels[clamp(index + direction, 1, #zoomLevels)]
			if zoom == state.zoom and state.mode == "width" then return false end
			state.zoom, state.mode = zoom, "width"
			state.scale = clamp(math.floor(2000 * zoom / 100), 1000, 4096)
			state.revision += 1
			if state.refresh then state.refresh() end
			return true
		end
		if name == "r" then if state.refresh then state.refresh() end return false end
		local page = state.page
		if name == "left" or name == "p" or name == "page_up" then page -= 1
		elseif name == "right" or name == "n" or name == "page_down" then page += 1
		elseif name == "home" then page = 1
		elseif name == "end" then page = state.pages or page
		else return false end
		page = clamp(page, 1, state.pages or math.max(1, page))
		if page == state.page then return false end
		state.page = page
		state.revision += 1
		if state.refresh then state.refresh() end
		return true
	end,
	save = function(state)
		return { path = state.path, page = state.page, mode = state.mode, zoom = state.zoom }
	end,
})
