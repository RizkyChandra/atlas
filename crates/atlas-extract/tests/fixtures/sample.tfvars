region        = "us-east-1"
instance_type = "t3.micro"
tags = {
  Environment = "prod"
  Team        = "platform"
}
subnet_ids = ["subnet-a", "subnet-b"]
