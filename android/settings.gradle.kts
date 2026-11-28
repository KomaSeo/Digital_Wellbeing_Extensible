// Responsibility: The Gradle build of the phone app (the owner: "브라우저 말고 휴대폰 앱으로 만들자").
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}
dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}
rootProject.name = "digital-wellbeing-phone"
include(":app")
