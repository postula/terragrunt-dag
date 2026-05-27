locals {
  cloud = values.cloud
}

stack "k8s" {
  source = "../../../../_modules/template"
  path   = "k8s"

  values = {
    cluster = format("%s-%s", local.cloud, values.env)
  }
}
