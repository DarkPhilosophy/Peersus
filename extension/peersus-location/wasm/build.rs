use std::{env, fmt::Write, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=src/world/land.txt");
    let mut rings = String::from("&[\n");
    for line in include_str!("src/world/land.txt")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
    {
        rings.push_str("&[\n");
        let (mut east, mut north) = (0_i32, 0_i32);
        for delta in line.split_whitespace() {
            let (dx, dy) = delta.split_once(',').expect("embedded land coordinates");
            east += dx.parse::<i32>().expect("embedded longitude delta");
            north += dy.parse::<i32>().expect("embedded latitude delta");
            let (sin_lon, cos_lon) = (f64::from(east) / 100.).to_radians().sin_cos();
            let (sin_lat, cos_lat) = (f64::from(north) / 100.).to_radians().sin_cos();
            let (x, y, z) = (
                (cos_lat * cos_lon) as f32,
                (cos_lat * sin_lon) as f32,
                sin_lat as f32,
            );
            // Precompute the vectors instead of parsing and projecting the asset on the first page.
            writeln!(
                rings,
                "(f32::from_bits({}), f32::from_bits({}), f32::from_bits({})),",
                x.to_bits(),
                y.to_bits(),
                z.to_bits()
            )
            .expect("writing a string");
        }
        rings.push_str("],\n");
    }
    rings.push_str("]\n");
    let path = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
    fs::write(path.join("land_vectors.rs"), rings).expect("write bundled coastline vectors");
}
