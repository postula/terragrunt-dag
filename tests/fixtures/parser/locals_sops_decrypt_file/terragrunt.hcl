locals {
  secrets = yamldecode(sops_decrypt_file("../secrets.yaml"))
}
