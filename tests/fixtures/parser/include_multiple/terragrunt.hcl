include "root" {
  path = find_in_parent_folders()
}

include "env" {
  path = "../env.hcl"
}

include "region" {
  path = find_in_parent_folders("region.hcl")
}
