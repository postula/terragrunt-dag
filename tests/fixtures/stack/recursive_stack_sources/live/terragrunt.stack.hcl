stack "consul" {
  source = "${get_repo_root()}/stacks/consul"
  path   = "consul"
}

stack "vault" {
  source = "${get_repo_root()}/stacks/vault"
  path   = "vault"
}
