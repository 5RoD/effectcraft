//! The wasmi-backed [`EffectPlugin`] (plug-in API v1 ABI, see the crate docs).

use std::sync::Mutex;

use wasmi::{Config, Engine, Instance, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder, TypedFunc, WasmParams, WasmResults};

use crate::{EffectPlugin, PLUGIN_API_VERSION, PluginFrame, PluginManifest, PluginParams};

/// Instruction budget per frame: a fixed allowance plus some per pixel. Generous for per-pixel
/// work, but a runaway loop stops instead of hanging the render.
const FUEL_BASE: u64 = 50_000_000;
const FUEL_PER_PIXEL: u64 = 20_000;
/// The most linear memory a plug-in instance may have (a 4K float frame is ~130 MB).
const MAX_MEMORY: usize = 1 << 30;
/// The longest manifest read.
const MAX_MANIFEST: i32 = 1 << 20;
/// One bounded host transfer: a supported RGBA-f32 frame plus 256 four-value parameters.
const MAX_TRANSFER: usize = 32 * 1024 * 1024 * 16 + 256 * 4 * 8;

#[derive(Debug)]
struct TransferPlan {
    params_bytes: usize,
    total: usize,
    fuel: u64,
}

impl TransferPlan {
    fn new(pixels: usize, params: usize) -> Result<Self, String> {
        let params_bytes = params.checked_mul(8).ok_or("plug-in parameter byte count overflow")?;
        let pixel_bytes = pixels.checked_mul(16).ok_or("plug-in pixel byte count overflow")?;
        let total = params_bytes.checked_add(pixel_bytes).ok_or("plug-in transfer byte count overflow")?;
        let pixels = u64::try_from(pixels).map_err(|_| "plug-in pixel count exceeds its supported address space")?;
        if params > 256 * 4 || pixels > effectcraft_effects::plugin::MAX_PLUGIN_PIXELS || total > MAX_TRANSFER || total > i32::MAX as usize {
            return Err(format!("plug-in transfer exceeds the {MAX_TRANSFER}-byte host limit"));
        }
        let fuel = pixels.checked_mul(FUEL_PER_PIXEL).and_then(|n| n.checked_add(FUEL_BASE)).ok_or("plug-in fuel count overflow")?;
        Ok(Self { params_bytes, total, fuel })
    }
}

fn transfer_bytes(len: usize, limit: usize) -> Result<Vec<u8>, String> {
    if len > limit || len > isize::MAX as usize {
        return Err("plug-in host byte allocation exceeds its limit".into());
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(len).map_err(|e| format!("cannot allocate {len} plug-in host bytes: {e}"))?;
    bytes.resize(len, 0);
    Ok(bytes)
}

type RenderFn = TypedFunc<(i32, i32, i32, i32, i32, f64, f64), i32>;

/// One instantiated module (renders running on several threads each take their own).
struct Inst {
    store: Store<StoreLimits>,
    instance: Instance,
    memory: Memory,
    alloc: TypedFunc<i32, i32>,
    render: RenderFn,
}

impl Inst {
    fn func<P: WasmParams, R: WasmResults>(&self, name: &str) -> Result<TypedFunc<P, R>, String> {
        self.instance.get_typed_func::<P, R>(&self.store, name).map_err(|_| missing(name))
    }
}

fn missing(name: &str) -> String {
    format!("the module exports no `{name}` (or with the wrong signature)")
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn instantiate(engine: &Engine, module: &Module) -> Result<Inst, String> {
    let mut store = Store::new(engine, StoreLimitsBuilder::new().memory_size(MAX_MEMORY).instances(1).build());
    store.limiter(|l| l);
    store.set_fuel(FUEL_BASE).map_err(err)?;
    let linker = Linker::<StoreLimits>::new(engine);
    let instance = linker.instantiate_and_start(&mut store, module).map_err(|e| format!("instantiation failed: {e}"))?;
    let memory = instance.get_memory(&store, "memory").ok_or("the module exports no `memory`")?;
    let alloc = instance.get_typed_func::<i32, i32>(&store, "ec_alloc").map_err(|_| missing("ec_alloc"))?;
    let render = instance.get_typed_func::<(i32, i32, i32, i32, i32, f64, f64), i32>(&store, "ec_render").map_err(|_| missing("ec_render"))?;
    Ok(Inst { store, instance, memory, alloc, render })
}

/// A loaded WebAssembly effect plug-in.
pub struct WasmPlugin {
    manifest: PluginManifest,
    source: String,
    engine: Engine,
    module: Module,
    /// Idle instances.
    pool: Mutex<Vec<Inst>>,
}

impl WasmPlugin {
    pub fn new(bytes: &[u8], source: &str) -> Result<WasmPlugin, String> {
        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, bytes).map_err(|e| format!("not a valid WebAssembly module: {e}"))?;
        if let Some(i) = module.imports().next() {
            return Err(format!("plug-ins can't import anything (the module imports `{}.{}`)", i.module(), i.name()));
        }
        let mut inst = instantiate(&engine, &module)?;
        let version = inst.func::<(), i32>("ec_api_version")?;
        let v = version.call(&mut inst.store, ()).map_err(err)?;
        if v != PLUGIN_API_VERSION as i32 {
            return Err(format!("the module implements plug-in API {v}, this build implements API {PLUGIN_API_VERSION}"));
        }
        let ptr = inst.func::<(), i32>("ec_manifest_ptr")?.call(&mut inst.store, ()).map_err(err)?;
        let len = inst.func::<(), i32>("ec_manifest_len")?.call(&mut inst.store, ()).map_err(err)?;
        if !(0..=MAX_MANIFEST).contains(&len) {
            return Err(format!("bad manifest length {len}"));
        }
        let mut text = transfer_bytes(len as usize, MAX_MANIFEST as usize)?;
        inst.memory.read(&inst.store, ptr as u32 as usize, &mut text).map_err(|e| format!("manifest out of bounds: {e}"))?;
        let manifest: PluginManifest = serde_json::from_slice(&text).map_err(|e| format!("bad manifest JSON: {e}"))?;
        manifest.validate()?;
        Ok(WasmPlugin { manifest, source: source.to_string(), engine, module, pool: Mutex::new(vec![inst]) })
    }

    fn take(&self) -> Result<Inst, String> {
        if let Some(i) = self.pool.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pop() {
            return Ok(i);
        }
        instantiate(&self.engine, &self.module)
    }

    fn give_back(&self, inst: Inst) {
        let mut p = self.pool.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // An idle-instance cache is optional; allocation failure safely drops this instance.
        if p.try_reserve(1).is_ok() {
            p.push(inst);
        }
    }
}

impl EffectPlugin for WasmPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn source(&self) -> String {
        self.source.clone()
    }

    fn render(&self, frame: &mut PluginFrame, params: &PluginParams, time: f64) -> Result<(), String> {
        frame.validate(params, time)?;
        let plan = TransferPlan::new(frame.pixels.len(), params.flat.len())?;
        let mut inst = self.take()?;
        inst.store.set_fuel(plan.fuel).map_err(err)?;
        let base = inst.alloc.call(&mut inst.store, plan.total as i32).map_err(|e| format!("ec_alloc trapped: {e}"))? as u32 as usize;
        let end = base.checked_add(plan.total).ok_or("ec_alloc returned an overflowing buffer address")?;
        if end > inst.memory.data(&inst.store).len() {
            return Err("ec_alloc returned a buffer out of bounds".into());
        }
        if !base.is_multiple_of(8) {
            return Err("ec_alloc returned a buffer that is not 8-byte aligned".into());
        }
        let px_ptr = base.checked_add(plan.params_bytes).ok_or("plug-in pixel address overflow")?;
        // Parameters, then pixels (little-endian, as WebAssembly memory is).
        let mut buf = transfer_bytes(plan.total, MAX_TRANSFER)?;
        let param_buf = buf.get_mut(..plan.params_bytes).ok_or("plug-in parameter transfer range invalid")?;
        for (bytes, v) in param_buf.as_chunks_mut::<8>().0.iter_mut().zip(params.flat) {
            bytes.copy_from_slice(&v.to_le_bytes());
        }
        let pixel_buf = buf.get_mut(plan.params_bytes..).ok_or("plug-in pixel transfer range invalid")?;
        for (bytes, px) in pixel_buf.as_chunks_mut::<16>().0.iter_mut().zip(frame.pixels.iter()) {
            for (bytes, c) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(px) {
                bytes.copy_from_slice(&c.to_le_bytes());
            }
        }
        inst.memory.write(&mut inst.store, base, &buf).map_err(|e| format!("ec_alloc returned a buffer out of bounds: {e}"))?;
        let r = inst
            .render
            .call(&mut inst.store, (px_ptr as i32, frame.width as i32, frame.height as i32, base as i32, params.flat.len() as i32, time, frame.scale))
            .map_err(|e| format!("ec_render trapped: {e}"));
        let r = match r {
            Ok(0) => {
                let pixels = buf.get_mut(plan.params_bytes..).ok_or("plug-in output transfer range invalid")?;
                inst.memory.read(&inst.store, px_ptr, pixels).map_err(|e| format!("ec_render output out of bounds: {e}"))?;
                for (px, b) in frame.pixels.iter_mut().zip(pixels.as_chunks::<16>().0.iter()) {
                    for (c, v) in px.iter_mut().zip(b.as_chunks::<4>().0.iter()) {
                        *c = f32::from_le_bytes([v[0], v[1], v[2], v[3]]);
                    }
                }
                Ok(())
            }
            Ok(code) => Err(format!("ec_render returned error {code}")),
            Err(e) => Err(e),
        };
        if r.is_ok() {
            self.give_back(inst);
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_transfer_plan_rejects_capacity_overflow_without_allocating() {
        assert!(TransferPlan::new(usize::MAX, 0).unwrap_err().contains("overflow"));
        assert!(TransferPlan::new(0, usize::MAX).unwrap_err().contains("overflow"));
        assert!(TransferPlan::new(32 * 1024 * 1024 + 1, 0).is_err());
        assert!(TransferPlan::new(1, 1025).is_err());
        assert!(transfer_bytes(usize::MAX, MAX_TRANSFER).is_err());
        let plan = TransferPlan::new(32 * 1024 * 1024, 1024).unwrap();
        assert_eq!(plan.total, MAX_TRANSFER);
        assert_eq!(plan.params_bytes, 8192);
        let small = TransferPlan::new(8, 6).unwrap();
        assert_eq!(small.total, 48 + 128);
        assert_eq!(small.fuel, FUEL_BASE + 8 * FUEL_PER_PIXEL);
        assert_eq!(transfer_bytes(small.total, MAX_TRANSFER).unwrap(), vec![0; small.total]);
    }
}
