locals {
  cluster = values.cluster
}

unit "service" {
  source = "../../../../../../_modules/template"
  path   = "svc"

  values = {
    dep = format("../%s_base", local.cluster)
  }
}
