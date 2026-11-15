//! Miolo do Nimbo em Rust, sem interface: login no Firebase e conversa em tempo real com o banco.
//! É a base do app nativo (`desktop/rust-ui`); o site continua usando o SDK do Firebase.
//! Ver `docs/rust-app.md` (seção "App full Rust") para o plano e o estado de cada etapa.

pub mod auth;
pub mod community;
pub mod config;
pub mod room;
pub mod rtdb;

pub type Json = serde_json::Value;
