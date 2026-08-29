//! `BasisServerTests/Networking`: one test binary, one module per C# file. The hello-world
//! suites drive a real server — over iroh, and in the mixed-world suite over the LiteNetLib
//! protocol beside it; `lnl_transport_tests` exercises that transport on its own. The rest
//! exercise the server's state machines with offline stand-in peers.

mod avatar_flush_order_tests;
mod avatar_scene_audio_message_round_trip_tests;
mod basis_connection_lifecycle_tests;
mod basis_p2p_connection_lifecycle_tests;
mod control_and_resource_message_round_trip_tests;
mod hello_world_peer_message_tests;
mod hello_world_peer_stress_tests;
mod idle_suppression_tests;
mod join_broadcast_tests;
mod join_fill_size_benchmark;
mod join_snapshot_tier_tests;
mod lnl_transport_tests;
mod mixed_world_hello_tests;
mod net_data_reader_writer_tests;
mod p2p_broker_offload_tests;
mod p2p_link_health_tests;
