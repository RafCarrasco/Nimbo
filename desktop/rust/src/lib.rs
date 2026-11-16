//! Nimbo Windows host shell (CP1) with bridge v1 (CP2 S1), loopback login (CP2 S2), activation link and per-user scheme registration (CP2 S3) plus retained capture feasibility core; separate from the web graph.
pub mod bridge;
pub mod capture;
pub mod frame_route;
pub mod login;
pub mod loopback;
pub mod policy;
pub mod scheme;
pub mod shell;
pub mod sources;
