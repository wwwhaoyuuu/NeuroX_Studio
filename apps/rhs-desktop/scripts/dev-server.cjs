const fs = require("fs");
const http = require("http");
const path = require("path");

const root = path.resolve(__dirname, "..");
const useDist = process.argv.includes("--dist");
const publicRoot = path.join(root, useDist ? "dist" : "src");
const port = Number(process.env.RHS_UI_PORT || 1420);
const mime = {
  ".css": "text/css; charset=utf-8",
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".png": "image/png",
  ".svg": "image/svg+xml"
};

function resolveRequest(url) {
  const pathname = decodeURIComponent((url || "/").split("?")[0]);
  if (!useDist && pathname === "/vendor/lucide.js") {
    return path.join(root, "node_modules", "lucide", "dist", "umd", "lucide.js");
  }
  const relative = pathname === "/" ? "index.html" : pathname.replace(/^\/+/, "");
  const resolved = path.resolve(publicRoot, relative);
  return resolved.startsWith(publicRoot) ? resolved : null;
}

const server = http.createServer((request, response) => {
  const file = resolveRequest(request.url);
  if (!file || !fs.existsSync(file) || fs.statSync(file).isDirectory()) {
    response.writeHead(404, { "Content-Type": "text/plain; charset=utf-8" });
    response.end("Not found");
    return;
  }
  response.writeHead(200, {
    "Cache-Control": "no-store",
    "Content-Type": mime[path.extname(file)] || "application/octet-stream"
  });
  fs.createReadStream(file).pipe(response);
});

server.listen(port, "127.0.0.1", () => {
  console.log(`NeuroX Studio ${useDist ? "preview" : "dev"} server: http://127.0.0.1:${port}`);
});
