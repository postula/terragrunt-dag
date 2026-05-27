locals {
  customers = {
    alice = { region = "us-east" }
    bob   = { region = "us-west" }
  }
  groups = { for name, _ in local.customers : "team/${name}" => name }
}

unit "service" {
  source = "../../_modules/template"
  path   = "svc"

  values = {
    groups = local.groups
    dep    = "../base"
  }
}
