// The bakery's menu site. Settings come from .env (copy .env.example).
const express = require("express");

const app = express();
const name = process.env.SHOP_NAME || "Bakery";
const port = Number(process.env.PORT || 3000);

const menu = [
  { item: "Sourdough loaf", price: "6.50" },
  { item: "Cinnamon bun", price: "3.25" },
  { item: "Almond croissant", price: "3.75" },
];

app.use(express.static("public"));

app.get("/", (_req, res) => {
  const rows = menu
    .map((m) => `<li><span>${m.item}</span><span>€${m.price}</span></li>`)
    .join("");
  res.send(`<!doctype html><html><head><meta charset="utf-8"><title>${name}</title>
<link rel="stylesheet" href="/style.css"></head>
<body><main><h1>${name}</h1><p>Baked fresh every morning.</p><ul>${rows}</ul></main></body></html>`);
});

app.listen(port, () => console.log(`${name} is open on http://localhost:${port}`));
