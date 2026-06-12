fn main() {
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("vendored protoc is available");
    std::env::set_var("PROTOC", protoc);

    prost_build::Config::new()
        .compile_protos(&["proto/eos/trace/v1/trace.proto"], &["proto/eos/trace/v1"])
        .expect("trace protobuf schema compiles");
}
