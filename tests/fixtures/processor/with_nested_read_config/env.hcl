locals {
  base = read_terragrunt_config("base.hcl")
  env_data = yamldecode(file("env.yaml"))
}
