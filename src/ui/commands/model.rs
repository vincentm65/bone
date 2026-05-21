pub fn run(provider_label: &str, model_label: &str) -> String {
    format!("{} ({})", model_label, provider_label)
}
