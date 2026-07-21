fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure()
        .compile_protos(&["proto/cleverhans/v1/envelope.proto"], &["proto"])?;
    Ok(())
}
