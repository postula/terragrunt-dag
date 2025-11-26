locals {
  global_config = yamldecode(file("config.yaml"))
}
