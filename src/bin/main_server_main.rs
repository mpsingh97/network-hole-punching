use network_hole_punching::main_server::run_main_server;

#[tokio::main]
async fn main() {
    run_main_server().await;
}
