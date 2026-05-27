terraform {
  source = "../../../../_modules/oauth"
}

dependency "k8s" {
  config_path = values.upstream_path
}
