terraform {
  source = "git::https://github.com/example/module-b.git"
}

dependency "c" {
  config_path = "../c"
}

# This creates a watch file that should cascade to A when cascade=true
locals {
  config = yamldecode(file("../config.yaml"))
}
