unit "api" {
  source = "${get_repo_root()}/modules/api"
  path   = "api"
}

unit "worker" {
  source = "${get_repo_root()}/modules/worker"
  path   = "worker"
}
