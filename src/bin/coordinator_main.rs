use std::env;

use network_hole_punching::coordinator::Coordinator;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <main_server_addr>", args[0]);
        return;
    }

    let main_server_addr = &args[1];

    let coordinator = Coordinator::new();
    coordinator.run("0.0.0.0:5000", main_server_addr).await;
}
