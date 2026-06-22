docker run -it --rm --name sofa-rust-dev \
  -v "$(pwd)":/workspace \
  -v claude-config:/root/.claude \
  rust-devcontainer
