const KEY = "pinboard.notes";
const board = document.getElementById("board");
const form = document.getElementById("new-note");
const input = document.getElementById("note-text");

let notes = JSON.parse(localStorage.getItem(KEY) || "null") || [
  "Buy coffee beans",
  "Call the plumber about the kitchen tap",
  "Ideas for the weekend: hike, farmers market",
];

function save() {
  localStorage.setItem(KEY, JSON.stringify(notes));
}

function render() {
  board.innerHTML = "";
  for (const text of notes) {
    const el = document.createElement("article");
    el.className = "note";
    el.textContent = text;
    board.appendChild(el);
  }
}

form.addEventListener("submit", (event) => {
  event.preventDefault();
  const text = input.value.trim();
  if (!text) return;
  notes.unshift(text);
  input.value = "";
  save();
  render();
});

render();
