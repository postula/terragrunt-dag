locals {
  unseal_type = "shamir"
}

unit "core" {
  source = "${get_repo_root()}/modules/vault/core"
  path   = "core"
}

unit "unseal" {
  source = "${get_repo_root()}/modules/vault/unseal/${local.unseal_type}"
  path   = "unseal"
}
