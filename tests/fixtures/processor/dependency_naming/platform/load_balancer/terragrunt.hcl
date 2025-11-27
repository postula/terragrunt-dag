terraform {
  source = "git::https://github.com/example/lb-module.git"
}

dependency "vpc" {
  config_path = "../vpc"
}
