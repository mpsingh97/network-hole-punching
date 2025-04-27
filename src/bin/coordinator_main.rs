use network_hole_punching::coordinator::Coordinator;

#[tokio::main]
async fn main() {
    let coordinator = Coordinator::new();
    coordinator.run("0.0.0.0:5000").await;
}
