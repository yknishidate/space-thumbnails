fn main() {
    let path = std::env::args().nth(1).expect("usage: read <file.abc>");
    let p = std::path::Path::new(&path);
    // path-based
    match alembic_sys::read_mesh(p) {
        Ok(m) => println!("[path]   vertices: {}, triangles: {}", m.vertex_count(), m.indices.len()/3),
        Err(e) => { eprintln!("[path] error: {e}"); std::process::exit(1); }
    }
    // memory-based (the provider path)
    let bytes = std::fs::read(p).unwrap();
    match alembic_sys::read_mesh_from_memory(&bytes) {
        Ok(m) => println!("[memory] vertices: {}, triangles: {}", m.vertex_count(), m.indices.len()/3),
        Err(e) => { eprintln!("[memory] error: {e}"); std::process::exit(1); }
    }
}
