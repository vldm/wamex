#!/usr/bin/env bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"
EXAMPLE_DIR="$ROOT_DIR/crates/example"
OUTPUT_DIR="$SCRIPT_DIR/dist"

echo "Building example crate..."
cargo build \
    --manifest-path="$EXAMPLE_DIR/Cargo.toml" \
    --target wasm32-unknown-unknown \
    --release \
    --features "split"

echo "Finding built wasm file..."
WASM_FILE=$(cargo build \
    --manifest-path="$EXAMPLE_DIR/Cargo.toml" \
    --target wasm32-unknown-unknown \
    --release \
    --features "split" \
    --message-format=json | \
    jq -r 'select(.reason == "compiler-artifact" and .target.name == "wamex_example") | .filenames[0]')

echo "Built WASM file: $WASM_FILE"

echo "Splitting WASM modules..."
SPLIT_TEMP_DIR="$OUTPUT_DIR/split_tmp"
rm -rf "$SPLIT_TEMP_DIR"
mkdir -p "$SPLIT_TEMP_DIR"

cargo run \
    --manifest-path="$ROOT_DIR/Cargo.toml" \
    -p wamex-cli \
    -- split "$WASM_FILE" "$SPLIT_TEMP_DIR"

echo "Running wasm-bindgen..."
BINDGEN_DIR="$OUTPUT_DIR/bindgen"
rm -rf "$BINDGEN_DIR"
mkdir -p "$BINDGEN_DIR"

wasm-bindgen \
    "$SPLIT_TEMP_DIR/main.wasm" \
    --out-dir "$BINDGEN_DIR" \
    --target web \
    --no-demangle \
    --keep-lld-exports

echo "Copying split modules..."
for file in "$SPLIT_TEMP_DIR"/*.wasm; do
    filename=$(basename "$file")
    if [ "$filename" != "main.wasm" ]; then
        cp "$file" "$BINDGEN_DIR/"
    fi
done

echo "Creating index.html..."
cat > "$BINDGEN_DIR/index.html" <<'EOF'
<!doctype html>
<html>
  <head>
    <meta content="text/html;charset=utf-8" http-equiv="Content-Type" />
    <script type="module" src="index.js"></script>
  </head>
  <body>
    <form id="form">
      <input style="width: 100%" type="text" id="url" />
      <input type="submit" />
    </form>
    <textarea id="result" style="width: 100%" rows="10"></textarea>
  </body>
</html>
EOF

echo "Creating index.js..."
cat > "$BINDGEN_DIR/index.js" <<'EOF'
import initializeWasm, * as main from "./main.js";

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
EOF

echo "Build complete! Output in: $BINDGEN_DIR"
echo ""
echo "To serve the app, run:"
echo "  cd $BINDGEN_DIR && python3 -m http.server 8080"
