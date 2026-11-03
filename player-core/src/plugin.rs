//! Plugin trait + static registry (Milestone 4 builds the first plugins on
//! this seam; dynamic `.so` loading is Milestone 6).

use crate::engine::{PlaybackEngine, PlayerEvent};

/// What a plugin may do: read state through the engine, never mutate mpv
/// behind the core's back.
pub struct PluginContext<'a> {
    pub engine: &'a dyn PlaybackEngine,
}

pub trait PlayerPlugin: Send + Sync {
    fn name(&self) -> &str;
    fn on_event(&self, _ctx: &PluginContext<'_>, _event: &PlayerEvent) {}
}

#[derive(Default)]
pub struct PluginRegistry {
    plugins: Vec<Box<dyn PlayerPlugin>>,
}

impl PluginRegistry {
    pub fn register(&mut self, plugin: Box<dyn PlayerPlugin>) {
        tracing::info!(plugin = plugin.name(), "plugin registered");
        self.plugins.push(plugin);
    }

    pub fn dispatch(&self, engine: &dyn PlaybackEngine, event: &PlayerEvent) {
        let ctx = PluginContext { engine };
        for plugin in &self.plugins {
            plugin.on_event(&ctx, event);
        }
    }

    pub fn plugin_names(&self) -> impl Iterator<Item = &str> {
        self.plugins.iter().map(|p| p.name())
    }
}
