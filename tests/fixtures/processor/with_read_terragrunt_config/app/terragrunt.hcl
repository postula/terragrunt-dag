terraform {
  source = "../../modules//app"
}

locals {
  common = read_terragrunt_config("../common.hcl")
}

dependency "rds" {
  config_path = "../rds"
}
