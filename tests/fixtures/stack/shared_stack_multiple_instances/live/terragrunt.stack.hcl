# Three environments instantiating one shared stack file. Each materializes
# its own copy under live/.terragrunt-stack/<env>/.terragrunt-stack/.
stack "dev" {
  source = "${get_repo_root()}/stacks/app"
  path   = "dev"
}

stack "staging" {
  source = "${get_repo_root()}/stacks/app"
  path   = "staging"
}

stack "prod" {
  source = "${get_repo_root()}/stacks/app"
  path   = "prod"
}
