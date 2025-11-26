locals {
  # Function call inside another function call
  root_config = read_terragrunt_config(find_in_parent_folders("root.hcl"))
}
