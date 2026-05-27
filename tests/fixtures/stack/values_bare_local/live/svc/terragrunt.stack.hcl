locals {
  shared_dep = "../base"
}

unit "service" {
  source = "../../_modules/template"
  path   = "svc"

  values = {
    all_locals = local
  }
}
