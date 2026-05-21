pub fn run() -> String {
    [
        "/clear     — clear chat history",
        "/compact   — show context usage",
        "/help      — show this message",
        "/model     — show current model",
        "/provider  — show or switch provider (/provider <name>)",
        "/quit      — exit bone",
    ].join("\n")
}
