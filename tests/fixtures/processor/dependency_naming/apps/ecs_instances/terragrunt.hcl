terraform {
  source = "git::https://github.com/example/ecs-module.git"
}

dependency "vpc" {
  config_path = "../../platform/vpc"
}

dependency "load_balancer" {
  config_path = "../../platform/load_balancer"
}
