stack "platform" {
  source = "../_modules/template"
  path   = "azure/east-us"

  values = {
    cloud = "azure"
    env   = "prod"
  }
}
