tern.route.open(function(req, _cx)
	if string.match(string.lower(req.path), "%.pdf$") then
		return { block = "pdf.viewer", args = { req.path } }
	end
	return nil
end)
