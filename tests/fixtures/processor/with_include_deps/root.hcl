terraform {
  source = "../../modules//app"
}

locals {
  global_config = yamldecode(file("config.yaml"))
}
