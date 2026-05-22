terraform {
  source = "./${local.auth_type}"
}

include "root" {
  path = find_in_parent_folders("root.hcl")
}

locals {
  auth_type = values.auth_type
}
