# Has terraform block - should NOT be filtered out

terraform {
  source = "../modules/vpc"
}

locals {
  region = "us-east-1"
}
