# Example E2E Tests

Integration tests for the `example` crate using Cucumber and Thirtyfour for browser automation.

## Overview

These tests verify that the WASM module splitting functionality works correctly by:
1. Building the example crate as `wasm32-unknown-unknown`
2. Splitting the WASM module using `wamex-cli`
3. Generating JavaScript bindings with `wasm-bindgen`
4. Testing the application in a real browser to ensure all split modules load correctly

## Prerequisites

### Required Tools

- **Rust toolchain** with `wasm32-unknown-unknown` target:
  ```bash
  rustup target add wasm32-unknown-unknown
  ```

- **wasm-bindgen-cli** (must match version in example/Cargo.toml):
  ```bash
  cargo install wasm-bindgen-cli
  ```

- **jq** for JSON parsing in build script:
  ```bash
  # On macOS
  brew install jq

  # On Ubuntu/Debian
  sudo apt-get install jq
  ```

- **Python 3** for running the local HTTP server

- **Chrome/Chromium browser** (ChromeDriver will be downloaded automatically by thirtyfour's selenium-manager)

### What About ChromeDriver?

**You don't need to install ChromeDriver manually!** This test suite uses the `Thirtyfour-chromedriver` crate, which automatically downloads the correct ChromeDriver version for your installed Chrome browser and launches it as a subprocess when tests run.

## Running the Tests

### Automated Test Run

The easiest way to run all tests:

```bash
./run_tests.sh
```

This script will:
1. Build the example crate
2. Split the WASM modules
3. Generate wasm-bindgen bindings
4. Start a local HTTP server on port 8080
5. Run the Cucumber tests (ChromeDriver starts automatically)
6. Clean up all processes on exit

### Manual Test Run

If you prefer to run steps manually:

1. **Build and split modules:**
   ```bash
   ./build.sh
   ```

2. **Start the web server** (in one terminal):
   ```bash
   cd dist/bindgen
   python3 -m http.server 8080
   ```

3. **Run tests** (in another terminal):
   ```bash
   cargo test
   ```

   ChromeDriver will be automatically downloaded and started by thirtyfour's selenium-manager when the tests begin.

## Test Scenarios

The test suite covers the following scenarios defined in `features/split_modules.feature`:

- **Static string module** - Tests loading a module that returns a static string reference
- **String build module** - Tests a module that builds a string at runtime
- **String build with shared const** - Tests modules that share constants
- **Async string module** - Tests async function support in split modules
- **Dynamic functions module** - Tests modules with dynamic dispatch
- **Dependent dynamic module** - Tests modules that depend on other split modules
- **Lifetime module** - Tests functions with lifetime parameters
- **Fallback behavior** - Tests that unknown inputs are handled correctly

## Project Structure

```
e2e/
├── build.sh              # Builds WASM, splits modules, runs wasm-bindgen
├── run_tests.sh          # Runs complete test suite with setup/teardown
├── Cargo.toml            # Test dependencies (cucumber, fantoccini)
├── features/
│   └── split_modules.feature  # Gherkin test scenarios
├── tests/
│   ├── app_suite.rs      # Test runner
│   └── fixtures/
│       ├── mod.rs        # Module exports
│       ├── find.rs       # Element finder helpers
│       ├── action.rs     # User action helpers
│       ├── check.rs      # Assertion helpers
│       └── world/
│           ├── mod.rs           # World setup and browser client
│           ├── action_steps.rs  # Given/When step implementations
│           └── check_steps.rs   # Then step implementations
└── dist/                 # Build output (generated)
    ├── split_tmp/        # Split WASM modules
    └── bindgen/          # wasm-bindgen output + HTML/JS
```

## Configuration

### Server Port

The app is served on `http://127.0.0.1:8080` by default. To change this:
- Update `HOST` constant in `tests/fixtures/world/mod.rs`
- Update the port in `build.sh` and `run_tests.sh`

### ChromeDriver Port

ChromeDriver runs on port 9515 (thirtyfour's default) by default. To change this:
- Update `WEBDRIVER_URL` constant in `tests/fixtures/world/mod.rs`
- Update the URL in the `start_webdriver_process` call

### Headless Mode

Tests run in headless Chrome by default. To see the browser:
- Change `caps.set_headless()?;` to `caps.set_headless()?;` and pass `false` instead in `tests/fixtures/world/mod.rs`

## Troubleshooting

### ChromeDriver Download Fails

If Thirtyfour-chromedriver fails to download ChromeDriver:
- Ensure you have a working internet connection
- Check that Chrome/Chromium is installed on your system
- The library will automatically detect your Chrome version and download the matching ChromeDriver
- On first run, you may see download progress messages - this is normal
- ChromeDriver binaries are cached in `~/.chromedriver/` directory

### Module Loading Failures

If tests fail with module loading errors:
- Check the browser console (disable headless mode)
- Verify all split modules are in the `dist/bindgen/` directory
- Ensure the wasm-bindgen version matches between CLI and runtime

### Build Script Fails

If `build.sh` fails:
- Ensure `jq` is installed for JSON parsing
- Check that the wasm32-unknown-unknown target is installed
- Verify wasm-bindgen-cli is installed and accessible

### Chrome Version Mismatch

If you see errors about Chrome version mismatches:
- Update your Chrome/Chromium browser to the latest version
- Delete the `~/.chromedriver/` directory to clear cached binaries
- The Thirtyfour-chromedriver library will download the correct ChromeDriver for your browser version on next run

## CI Integration

For CI environments, the Thirtyfour-chromedriver library works automatically. Just ensure:
- Chrome/Chromium is installed in your CI image
- Network access is available for downloading ChromeDriver on first run
- The CI environment has permissions to execute downloaded binaries

Example GitHub Actions setup:
```yaml
- name: Install Chrome
  run: |
    sudo apt-get update
    sudo apt-get install -y chromium-browser
- name: Run E2E Tests
  run: cd crates/example/e2e && ./run_tests.sh
```

## References

- Cucumber framework: https://github.com/cucumber-rs/cucumber
- Thirtyfour browser automation: https://github.com/vrtgs/thirtyfour
- Thirtyfour documentation: https://docs.rs/thirtyfour
- Thirtyfour-chromedriver: https://crates.io/crates/Thirtyfour-chromedriver
- Similar setup in Leptos: https://github.com/leptos-rs/leptos/tree/main/examples/lazy_routes/e2e
