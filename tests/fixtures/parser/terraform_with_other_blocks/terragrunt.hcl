include "root" {
  path = find_in_parent_folders()
}

terraform {
  source = "../modules/app"
}

dependency "vpc" {
  config_path = "../vpc"
}
