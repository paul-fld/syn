// Empêche l'ouverture d'une console sur Windows en release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    syn_app::run();
}
