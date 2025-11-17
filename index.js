import initializeWasm, * as main from "./test-new-out-release/bindgen/main.js";

const url = document.getElementById("url");
const form = document.getElementById("form");
const result = document.getElementById("result");
form.addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    await initializeWasm();
    const urlValue = url.value;
    const decoded = await main.print_lazy_loaded_string(urlValue);

    result.textContent = decoded;
  } catch (e) {
    result.textContent = "Error: " + e.toString();
  }
});
