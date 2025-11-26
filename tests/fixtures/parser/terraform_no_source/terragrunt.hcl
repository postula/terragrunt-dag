# terraform block without source (other config only)
terraform {
  extra_arguments "common_vars" {
    commands = ["plan", "apply"]
  }
}
