include "root" {
  path = find_in_parent_folders()
}

locals {
  common = read_terragrunt_config("../common.hcl")
  data   = yamldecode(file("../data.yaml"))
}

dependency "vpc" {
  config_path = "../vpc"
}

terraform {
  source = "../modules/app"
}
