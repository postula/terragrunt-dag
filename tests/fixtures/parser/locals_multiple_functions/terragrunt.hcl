locals {
  common  = read_terragrunt_config("../common.hcl")
  env     = read_terragrunt_config("../env.hcl")
  config  = yamldecode(file("../config.yaml"))
  secrets = yamldecode(sops_decrypt_file("../secrets.yaml"))
  tfvars  = read_tfvars_file("../terraform.tfvars")
}
