fn main() {
    let caps = basis_network_core::transport::host_udp_capabilities::HostUdpCapabilities::get();
    println!("{}", caps.report());
    println!("{}", caps.json());
}
