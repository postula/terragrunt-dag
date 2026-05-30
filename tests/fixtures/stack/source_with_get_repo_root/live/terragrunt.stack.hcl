locals {
  unseal_type = "gcp"
}

unit "unseal" {
  source = "${get_repo_root()}/modules/vault/unseal/${local.unseal_type}"
  path   = "unseal"
}
