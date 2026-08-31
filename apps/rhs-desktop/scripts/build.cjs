const fs = require("fs");
const path = require("path");

const root = path.resolve(__dirname, "..");
const source = path.join(root, "src");
const output = path.join(root, "dist");
const lucide = path.join(root, "node_modules", "lucide", "dist", "umd", "lucide.js");

if (!fs.existsSync(lucide)) {
  throw new Error("lucide is missing; run npm install in apps/rhs-desktop");
}

fs.rmSync(output, { recursive: true, force: true });
fs.mkdirSync(path.join(output, "vendor"), { recursive: true });
for (const entry of fs.readdirSync(source, { withFileTypes: true })) {
  const from = path.join(source, entry.name);
  const to = path.join(output, entry.name);
  if (entry.isDirectory()) {
    fs.cpSync(from, to, { recursive: true });
  } else {
    fs.copyFileSync(from, to);
  }
}
fs.copyFileSync(lucide, path.join(output, "vendor", "lucide.js"));
console.log(`Built RHS Studio frontend at ${output}`);
