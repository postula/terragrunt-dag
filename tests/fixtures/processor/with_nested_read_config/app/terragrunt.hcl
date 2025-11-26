locals {
  env = read_terragrunt_config("../env.hcl")
}

dependency "db" {
  config_path = "../db"
}
