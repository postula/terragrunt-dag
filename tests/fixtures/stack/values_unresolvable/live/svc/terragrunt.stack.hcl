unit "main" {
  source = "../../_modules/template"
  path   = "main"

  values = {
    # Triggers an HCL eval error: `local.does_not_exist` is undefined.
    something = local.does_not_exist
  }
}
