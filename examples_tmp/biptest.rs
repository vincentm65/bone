#[tokio::main]
async fn main() {
    let r = tokio::task::spawn_blocking(|| {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async { 42 })
        })
    }).await;
    println!("result: {:?}", r);
}
