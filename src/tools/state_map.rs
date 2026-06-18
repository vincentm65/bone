use std::collections::HashMap;

/// In-memory tool state keyed by (source, sub_key).
///
/// - Single-entry tools (task_list): source="task_list", sub_key="default"
#[derive(Clone, Debug, Default)]
pub struct ToolStateMap {
    /// source -> sub_key -> opaque state blob
    entries: HashMap<String, HashMap<String, String>>,
}

impl ToolStateMap {
    pub fn set(&mut self, source: &str, sub_key: &str, value: String) {
        self.entries
            .entry(source.to_string())
            .or_default()
            .insert(sub_key.to_string(), value);
    }

    pub fn remove(&mut self, source: &str, sub_key: &str) {
        if let Some(inner) = self.entries.get_mut(source) {
            inner.remove(sub_key);
            if inner.is_empty() {
                self.entries.remove(source);
            }
        }
    }

    pub fn get(&self, source: &str, sub_key: &str) -> Option<&str> {
        self.entries.get(source)?.get(sub_key).map(|s| s.as_str())
    }

    /// Clear everything (session reset).
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}
