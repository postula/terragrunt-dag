unit "main" {
  source = "../../../_modules/template"
  path   = "main"

  values = {
    dep = "../../sibling"
  }
}
