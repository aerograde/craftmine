// One of the game's threads (horizon-os's `workers`): instantiates the game's
// module on the page's shared memory, then runs the thread it was handed.
import init, { horizon_worker_entry } from "./pkg/horizon-client.js";

self.onmessage = async ({ data }) => {
    self.onmessage = null;
    // The device facts live on the page, and a worker has its own `globalThis`:
    // `horizon_os` reads them here, so copy the page's values across before the
    // module starts (they can't be looked up again — `navigator.deviceMemory`
    // is the page's, and the page is the only thing that fetched the public IP).
    for (const key of ["horizonDeviceMemory", "horizonHardwareConcurrency", "horizonPublicIp"]) {
        if (data[key] !== undefined) globalThis[key] = data[key];
    }
    await init({ module_or_path: data.module, memory: data.memory, thread_stack_size: data.stack });
    horizon_worker_entry(data.ptr);
    self.close();
};
