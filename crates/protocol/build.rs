fn main() {
    println!("cargo:rerun-if-changed=proto");
    let fds = protox::compile(["remote.proto"], ["proto"]).expect("compile remote.proto");
    prost_build::Config::new()
        .compile_fds(fds)
        .expect("generate protobuf code");
}
