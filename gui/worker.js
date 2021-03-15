// shim to work around a wasm-bindgen thing where it expects either AudioContext or
// webkitAudioContext to exist, but neither does in web workers.
class AudioContext {}
importScripts("./pkg/gui.js");
const { _web_worker_entry_point } = wasm_bindgen;
async function run() {
    await wasm_bindgen("./pkg/gui_bg.wasm");
    _web_worker_entry_point(self);
}
run();