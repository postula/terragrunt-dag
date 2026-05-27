include "root" {
  path = find_in_parent_folders("root.hcl")
}

terraform {
  source = "../../../_modules/consumer"
}

dependency "azure_auth" {
  config_path = "../auth/.terragrunt-stack/azure/alpha"
}
