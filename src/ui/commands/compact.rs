use crate::chat::{Context, Message};

pub fn run(messages: &[Message], context: &Context) -> String {
    let budget = context.budget();
    let used: usize = messages.iter().map(|m| Context::estimate_tokens(&m.content)).sum();
    let pct = used * 100 / budget.max(1);
    format!("Context: {used}/{budget} tokens ({pct}%)")
}
